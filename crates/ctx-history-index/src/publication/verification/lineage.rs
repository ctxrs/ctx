use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use ctx_history_core::SessionRelationshipKind;
use tantivy::{schema::IndexRecordOption, DocAddress, DocSet, Searcher, Term, TERMINATED};

use crate::{
    hex,
    query::{self, CompactEventOrigin, CompactIdentity},
    Fields, IndexError, Result,
};

use super::{
    note_candidate_lineage_decode, note_candidate_lineage_spill,
    spill::{IdentityDeltaSpill, IdentityKeySpill, SpillVerificationIdentities, VerificationSpill},
};

const MAX_LINEAGE_DEPTH: usize = 1_024;
const MAX_SESSION_CACHE_ENTRIES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionRelationship {
    parent: Option<CompactIdentity>,
    root: CompactIdentity,
    kind: SessionRelationshipKind,
}

pub(super) fn verify_incremental_lineage(
    searcher: &Searcher,
    base_searcher: &Searcher,
    fields: Fields,
    changed_segments: &[usize],
    changed: &IdentityDeltaSpill,
) -> Result<()> {
    let changed_segments = changed_segments.iter().copied().collect::<HashSet<_>>();
    let retired = retired_identity_delta(searcher, base_searcher, fields)?;
    let mut affected_sessions = IdentityKeySpill::create()?;
    changed.for_each(|identities| affected_sessions.push(identities.session))?;
    retired.for_each(|identities| affected_sessions.push(identities.session))?;

    let mut resolver = IncrementalResolver::new(searcher, fields, changed_segments);
    affected_sessions.for_each_unique(|session| {
        let candidate = resolver.resolve_session(session)?;
        let base = resolve_session_indexed(base_searcher, fields, session, None)?;
        if candidate.is_some() {
            resolver.verify_session_chain(session)?;
        }
        if candidate != base && base.is_some() {
            resolver.verify_inverse_session_references(session)?;
            resolver.verify_descendant_copy_references(session)?;
        }
        Ok(())
    })?;

    changed.for_each(|identities| resolver.verify_changed_event(base_searcher, identities))?;
    retired.for_each(|identities| resolver.verify_retired_event(base_searcher, identities))
}

fn retired_identity_delta(
    searcher: &Searcher,
    base_searcher: &Searcher,
    fields: Fields,
) -> Result<IdentityDeltaSpill> {
    let candidate_segments = searcher.segment_readers();
    let candidate_by_id = candidate_segments
        .iter()
        .enumerate()
        .map(|(ordinal, segment)| (segment.segment_id().uuid_string(), ordinal))
        .collect::<HashMap<_, _>>();
    let mut retired = IdentityDeltaSpill::create()?;
    for (base_ordinal, base_segment) in base_searcher.segment_readers().iter().enumerate() {
        let candidate = candidate_by_id
            .get(&base_segment.segment_id().uuid_string())
            .map(|ordinal| &candidate_segments[*ordinal]);
        if candidate
            .is_some_and(|segment| segment.num_deleted_docs() == base_segment.num_deleted_docs())
        {
            continue;
        }
        for doc_id in 0..base_segment.max_doc() {
            if base_segment.is_deleted(doc_id)
                || candidate.is_some_and(|segment| !segment.is_deleted(doc_id))
            {
                continue;
            }
            retired.push(indexed_identities(
                base_searcher,
                DocAddress::new(
                    u32::try_from(base_ordinal).map_err(|_| IndexError::CountOverflow)?,
                    doc_id,
                ),
                fields,
            )?)?;
            note_candidate_lineage_spill();
        }
    }
    Ok(retired)
}

struct IncrementalResolver<'a> {
    searcher: &'a Searcher,
    fields: Fields,
    changed_segments: HashSet<usize>,
    relationships: BoundedSessionCache<SessionRelationship>,
    valid_roots: BoundedSessionCache<CompactIdentity>,
}

impl<'a> IncrementalResolver<'a> {
    fn new(searcher: &'a Searcher, fields: Fields, changed_segments: HashSet<usize>) -> Self {
        Self {
            searcher,
            fields,
            changed_segments,
            relationships: BoundedSessionCache::default(),
            valid_roots: BoundedSessionCache::default(),
        }
    }

    fn resolve_session(&mut self, session: CompactIdentity) -> Result<Option<SessionRelationship>> {
        if let Some(relationship) = self.relationships.get(session) {
            return Ok(Some(relationship));
        }
        let relationship = resolve_session_indexed(
            self.searcher,
            self.fields,
            session,
            Some(&self.changed_segments),
        )?;
        if let Some(relationship) = relationship {
            self.relationships.insert(session, relationship);
        }
        Ok(relationship)
    }

    fn verify_session_chain(&mut self, start: CompactIdentity) -> Result<()> {
        if self.valid_roots.get(start).is_some() {
            return Ok(());
        }
        let mut seen = Vec::with_capacity(16);
        let mut current = start;
        let mut expected_root = None;
        for _ in 0..MAX_LINEAGE_DEPTH {
            if seen.contains(&current) {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "session relationship cycle",
                ));
            }
            if let Some(root) = self.valid_roots.get(current) {
                if expected_root.is_some_and(|expected| expected != root) {
                    return Err(IndexError::InvalidSessionRelationshipGraph(
                        "session chain disagrees on root identity",
                    ));
                }
                for session in seen {
                    self.valid_roots.insert(session, root);
                }
                return Ok(());
            }
            seen.push(current);
            let relationship = self.resolve_session(current)?.ok_or(
                IndexError::InvalidSessionRelationshipGraph("related session does not exist"),
            )?;
            match expected_root {
                None => expected_root = Some(relationship.root),
                Some(root) if root == relationship.root => {}
                Some(_) => {
                    return Err(IndexError::InvalidSessionRelationshipGraph(
                        "session chain disagrees on root identity",
                    ));
                }
            }
            if relationship.kind == SessionRelationshipKind::Root {
                if relationship.parent.is_some() || relationship.root != current {
                    return Err(IndexError::InvalidSessionRelationshipGraph(
                        "root relationship is not self-rooted",
                    ));
                }
                let root = relationship.root;
                for session in seen {
                    self.valid_roots.insert(session, root);
                }
                return Ok(());
            }
            current = relationship
                .parent
                .ok_or(IndexError::InvalidSessionRelationshipGraph(
                    "non-root relationship has no parent",
                ))?;
        }
        Err(IndexError::InvalidSessionRelationshipGraph(
            "session relationship depth exceeds bound",
        ))
    }

    fn verify_inverse_session_references(&mut self, changed: CompactIdentity) -> Result<()> {
        for field in [self.fields.parent_session_id, self.fields.root_session_id] {
            let term = Term::from_field_text(field, &changed.as_uuid().to_string());
            let mut sessions = IdentityKeySpill::create()?;
            for_each_term_posting(self.searcher, field, &term, |address| {
                sessions.push(indexed_identities(self.searcher, address, self.fields)?.session)?;
                Ok(())
            })?;
            sessions.for_each_unique(|session| self.verify_session_chain(session))?;
        }
        Ok(())
    }

    fn verify_descendant_copy_references(&mut self, changed: CompactIdentity) -> Result<()> {
        let mut frontier = IdentityKeySpill::create()?;
        frontier.push(changed)?;
        note_candidate_lineage_spill();
        for _ in 0..MAX_LINEAGE_DEPTH {
            let mut next = IdentityKeySpill::create()?;
            frontier.for_each_unique(|session| {
                self.verify_copies_in_session(session)?;
                self.collect_direct_descendants(session, &mut next)
            })?;
            if next.is_empty() {
                return Ok(());
            }
            frontier = next;
        }
        Err(IndexError::InvalidSessionRelationshipGraph(
            "session relationship depth exceeds bound",
        ))
    }

    fn verify_copies_in_session(&mut self, session: CompactIdentity) -> Result<()> {
        let session_term =
            Term::from_field_text(self.fields.session_id, &session.as_uuid().to_string());
        let copied_term =
            Term::from_field_text(self.fields.event_origin_kind, "copied_from_ancestor");
        let searcher = self.searcher;
        let fields = self.fields;
        for_each_term_intersection(
            searcher,
            fields.session_id,
            &session_term,
            fields.event_origin_kind,
            &copied_term,
            |address| self.verify_copy_chain(indexed_identities(searcher, address, fields)?),
        )
    }

    fn collect_direct_descendants(
        &mut self,
        parent: CompactIdentity,
        descendants: &mut IdentityKeySpill,
    ) -> Result<()> {
        let term =
            Term::from_field_text(self.fields.parent_session_id, &parent.as_uuid().to_string());
        let searcher = self.searcher;
        let fields = self.fields;
        for_each_term_posting(searcher, fields.parent_session_id, &term, |address| {
            let child = indexed_identities(searcher, address, fields)?.session;
            self.verify_session_chain(child)?;
            descendants.push(child)?;
            note_candidate_lineage_spill();
            Ok(())
        })
    }

    fn verify_changed_event(
        &mut self,
        base_searcher: &Searcher,
        changed: SpillVerificationIdentities,
    ) -> Result<()> {
        let candidate = resolve_event_indexed(self.searcher, self.fields, changed.event)?;
        if matches!(
            candidate.event_origin,
            CompactEventOrigin::CopiedFromAncestor { .. }
        ) {
            self.verify_copy_chain(candidate)?;
        }
        let base = resolve_event_indexed_optional(base_searcher, self.fields, changed.event)?;
        if base.is_some_and(|base| event_lineage_state(base) != event_lineage_state(candidate)) {
            self.verify_inverse_copy_references(changed.event)?;
        }
        Ok(())
    }

    fn verify_retired_event(
        &mut self,
        base_searcher: &Searcher,
        retired: SpillVerificationIdentities,
    ) -> Result<()> {
        let candidate = resolve_event_indexed_optional(self.searcher, self.fields, retired.event)?;
        let base = resolve_event_indexed(base_searcher, self.fields, retired.event)?;
        if candidate.map(event_lineage_state) != Some(event_lineage_state(base)) {
            if let Some(candidate) = candidate {
                if matches!(
                    candidate.event_origin,
                    CompactEventOrigin::CopiedFromAncestor { .. }
                ) {
                    self.verify_copy_chain(candidate)?;
                }
            }
            self.verify_inverse_copy_references(retired.event)?;
        }
        Ok(())
    }

    fn verify_inverse_copy_references(&mut self, event: CompactIdentity) -> Result<()> {
        let term = Term::from_field_text(
            self.fields.origin_event_identity_digest,
            &hex(&event.digest),
        );
        let mut copies = IdentityDeltaSpill::create()?;
        for_each_term_posting(
            self.searcher,
            self.fields.origin_event_identity_digest,
            &term,
            |address| {
                copies.push(indexed_identities(self.searcher, address, self.fields)?)?;
                Ok(())
            },
        )?;
        copies.for_each(|copy| self.verify_copy_chain(copy))
    }

    fn verify_copy_chain(&mut self, start: SpillVerificationIdentities) -> Result<()> {
        let mut seen = Vec::with_capacity(8);
        let mut current = start;
        for _ in 0..MAX_LINEAGE_DEPTH {
            if seen.contains(&current.event.digest) {
                return Err(IndexError::InvalidEventOriginGraph("copied-event cycle"));
            }
            seen.push(current.event.digest);
            let CompactEventOrigin::CopiedFromAncestor {
                ancestor_session,
                ancestor_event,
            } = current.event_origin
            else {
                return Ok(());
            };
            if !self.session_is_ancestor(current.session, ancestor_session)? {
                return Err(IndexError::InvalidEventOriginGraph(
                    "declared origin session is not an ancestor",
                ));
            }
            let target = resolve_event_indexed(self.searcher, self.fields, ancestor_event)?;
            if target.session != ancestor_session {
                return Err(IndexError::InvalidEventOriginGraph(
                    "origin event belongs to a different session",
                ));
            }
            current = target;
        }
        Err(IndexError::InvalidEventOriginGraph(
            "copied-event depth exceeds bound",
        ))
    }

    fn session_is_ancestor(
        &mut self,
        child: CompactIdentity,
        ancestor: CompactIdentity,
    ) -> Result<bool> {
        let mut seen = Vec::with_capacity(16);
        let mut current = child;
        for _ in 0..MAX_LINEAGE_DEPTH {
            if seen.contains(&current.digest) {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "session relationship cycle",
                ));
            }
            seen.push(current.digest);
            let relationship = self.resolve_session(current)?.ok_or(
                IndexError::InvalidSessionRelationshipGraph("related session does not exist"),
            )?;
            if relationship.kind == SessionRelationshipKind::Root {
                return Ok(false);
            }
            let parent = relationship
                .parent
                .ok_or(IndexError::InvalidSessionRelationshipGraph(
                    "non-root relationship has no parent",
                ))?;
            if parent == ancestor {
                return Ok(true);
            }
            current = parent;
        }
        Err(IndexError::InvalidSessionRelationshipGraph(
            "session relationship depth exceeds bound",
        ))
    }
}

struct BoundedSessionCache<T: Copy> {
    entries: HashMap<[u8; 32], T>,
}

impl<T: Copy> Default for BoundedSessionCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::with_capacity(MAX_SESSION_CACHE_ENTRIES),
        }
    }
}

impl<T: Copy> BoundedSessionCache<T> {
    fn get(&self, identity: CompactIdentity) -> Option<T> {
        self.entries.get(&identity.digest).copied()
    }

    fn insert(&mut self, identity: CompactIdentity, value: T) {
        if self.entries.len() == MAX_SESSION_CACHE_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert(identity.digest, value);
    }
}

fn indexed_identities(
    searcher: &Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<SpillVerificationIdentities> {
    note_candidate_lineage_decode();
    let identities = query::stored_verification_identities(searcher, address, fields)?;
    Ok(SpillVerificationIdentities {
        event: identities.event,
        session: identities.session,
        parent_session: identities.parent_session,
        root_session: identities.root_session,
        session_relationship: identities.session_relationship,
        event_origin: identities.event_origin,
        session_source_ordinal: 0,
    })
}

fn resolve_session_indexed(
    searcher: &Searcher,
    fields: Fields,
    session: CompactIdentity,
    changed_segments: Option<&HashSet<usize>>,
) -> Result<Option<SessionRelationship>> {
    let term = Term::from_field_text(fields.session_id, &session.as_uuid().to_string());
    let mut resolved = None;
    let mut decoded_retained = false;
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let inverted = segment.inverted_index(fields.session_id)?;
        let Some(term_info) = inverted.get_term_info(&term)? else {
            continue;
        };
        for_each_live_posting(&inverted, &term_info, segment_ord, segment, |address| {
            let changed = changed_segments.is_some_and(|segments| segments.contains(&segment_ord));
            if !changed && std::mem::replace(&mut decoded_retained, true) {
                return Ok(());
            }
            let identities = indexed_identities(searcher, address, fields)?;
            if identities.session != session {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "compact session identity collision",
                ));
            }
            let candidate = relationship_for(identities);
            match resolved {
                None => resolved = Some(candidate),
                Some(existing) if existing == candidate => {}
                Some(_) => {
                    return Err(IndexError::InvalidSessionRelationshipGraph(
                        "one session has contradictory relationship fields",
                    ));
                }
            }
            Ok(())
        })?;
    }
    Ok(resolved)
}

fn resolve_event_indexed_optional(
    searcher: &Searcher,
    fields: Fields,
    event: CompactIdentity,
) -> Result<Option<SpillVerificationIdentities>> {
    let term = Term::from_field_text(fields.event_id, &event.as_uuid().to_string());
    let mut resolved = None;
    for_each_term_posting(searcher, fields.event_id, &term, |address| {
        let candidate = indexed_identities(searcher, address, fields)?;
        if candidate.event != event || resolved.is_some() {
            return Err(IndexError::InvalidEventOriginGraph(
                "origin event identity is ambiguous",
            ));
        }
        resolved = Some(candidate);
        Ok(())
    })?;
    Ok(resolved)
}

fn resolve_event_indexed(
    searcher: &Searcher,
    fields: Fields,
    event: CompactIdentity,
) -> Result<SpillVerificationIdentities> {
    resolve_event_indexed_optional(searcher, fields, event)?.ok_or(
        IndexError::InvalidEventOriginGraph("origin event does not exist"),
    )
}

fn event_lineage_state(
    identities: SpillVerificationIdentities,
) -> (CompactIdentity, CompactEventOrigin) {
    (identities.session, identities.event_origin)
}

pub(super) fn verify_lineage(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
) -> Result<()> {
    verify_session_relationships(searcher, fields, spill)?;
    verify_copied_event_origins(searcher, fields, spill)
}

fn verify_session_relationships(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
) -> Result<()> {
    let segments = searcher.segment_readers();
    let inverted = segments
        .iter()
        .map(|segment| segment.inverted_index(fields.session_id))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = inverted
        .iter()
        .map(|index| index.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = tantivy::termdict::TermMerger::new(streams);
    let mut valid_roots = BoundedSessionCache::default();
    while merged.advance() {
        let mut session = None;
        let mut relationship = None;
        for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
            for_each_live_posting(
                &inverted[segment_ord],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    let identities = spill.record(address, "session_relationship")?;
                    match session {
                        None => session = Some(identities.session),
                        Some(existing) if existing == identities.session => {}
                        Some(_) => {
                            return Err(IndexError::InvalidSessionRelationshipGraph(
                                "compact session identity collision",
                            ));
                        }
                    }
                    let candidate = relationship_for(identities);
                    match relationship {
                        None => relationship = Some(candidate),
                        Some(existing) if existing == candidate => {}
                        Some(_) => {
                            return Err(IndexError::InvalidSessionRelationshipGraph(
                                "one session has contradictory relationship fields",
                            ));
                        }
                    }
                    Ok(())
                },
            )?;
        }
        let Some(session) = session else {
            continue;
        };
        verify_session_chain(searcher, fields, spill, session, &mut valid_roots)?;
    }
    Ok(())
}

fn verify_session_chain(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
    start: CompactIdentity,
    valid_roots: &mut BoundedSessionCache<CompactIdentity>,
) -> Result<()> {
    if valid_roots.get(start).is_some() {
        return Ok(());
    }
    let mut seen = Vec::with_capacity(16);
    let mut current = start;
    let mut expected_root = None;
    for _ in 0..MAX_LINEAGE_DEPTH {
        if seen.contains(&current.digest) {
            return Err(IndexError::InvalidSessionRelationshipGraph(
                "session relationship cycle",
            ));
        }
        if let Some(root) = valid_roots.get(current) {
            if expected_root.is_some_and(|expected| expected != root) {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "session chain disagrees on root identity",
                ));
            }
            for digest in seen {
                valid_roots.insert(CompactIdentity { digest }, root);
            }
            return Ok(());
        }
        seen.push(current.digest);
        let relationship = resolve_session(searcher, fields, spill, current)?;
        match expected_root {
            None => expected_root = Some(relationship.root),
            Some(root) if root == relationship.root => {}
            Some(_) => {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "session chain disagrees on root identity",
                ));
            }
        }
        if relationship.kind == SessionRelationshipKind::Root {
            if relationship.parent.is_some() || relationship.root != current {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "root relationship is not self-rooted",
                ));
            }
            let root = relationship.root;
            for digest in seen {
                valid_roots.insert(CompactIdentity { digest }, root);
            }
            return Ok(());
        }
        current = relationship.parent.unwrap_or(relationship.root);
    }
    Err(IndexError::InvalidSessionRelationshipGraph(
        "session relationship depth exceeds bound",
    ))
}

fn verify_copied_event_origins(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
) -> Result<()> {
    let term = Term::from_field_text(fields.event_origin_kind, "copied_from_ancestor");
    for_each_term_posting(searcher, fields.event_origin_kind, &term, |address| {
        let copied = spill.record(address, "event_origin")?;
        if !matches!(
            copied.event_origin,
            CompactEventOrigin::CopiedFromAncestor { .. }
        ) {
            return Err(IndexError::InvalidEventOriginGraph(
                "copied-origin posting disagrees with stored Core",
            ));
        }
        resolve_copy_chain(searcher, fields, spill, copied)
    })
}

fn resolve_copy_chain(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
    start: SpillVerificationIdentities,
) -> Result<()> {
    let mut seen = Vec::with_capacity(8);
    let mut current = start;
    for _ in 0..MAX_LINEAGE_DEPTH {
        if seen.contains(&current.event.digest) {
            return Err(IndexError::InvalidEventOriginGraph("copied-event cycle"));
        }
        seen.push(current.event.digest);
        let CompactEventOrigin::CopiedFromAncestor {
            ancestor_session,
            ancestor_event,
        } = current.event_origin
        else {
            return Ok(());
        };
        if !session_is_ancestor(searcher, fields, spill, current.session, ancestor_session)? {
            return Err(IndexError::InvalidEventOriginGraph(
                "declared origin session is not an ancestor",
            ));
        }
        let target = resolve_event(searcher, fields, spill, ancestor_event)?;
        if target.session != ancestor_session {
            return Err(IndexError::InvalidEventOriginGraph(
                "origin event belongs to a different session",
            ));
        }
        current = target;
    }
    Err(IndexError::InvalidEventOriginGraph(
        "copied-event depth exceeds bound",
    ))
}

fn session_is_ancestor(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
    child: CompactIdentity,
    ancestor: CompactIdentity,
) -> Result<bool> {
    let mut seen = Vec::with_capacity(16);
    let mut current = child;
    for _ in 0..MAX_LINEAGE_DEPTH {
        if seen.contains(&current.digest) {
            return Err(IndexError::InvalidSessionRelationshipGraph(
                "session relationship cycle",
            ));
        }
        seen.push(current.digest);
        let relationship = resolve_session(searcher, fields, spill, current)?;
        if relationship.kind == SessionRelationshipKind::Root {
            return Ok(false);
        }
        let next = relationship.parent.unwrap_or(relationship.root);
        if next == ancestor {
            return Ok(true);
        }
        current = next;
    }
    Err(IndexError::InvalidSessionRelationshipGraph(
        "session relationship depth exceeds bound",
    ))
}

fn resolve_event(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
    event: CompactIdentity,
) -> Result<SpillVerificationIdentities> {
    let term = Term::from_field_text(fields.event_id, &event.as_uuid().to_string());
    let mut resolved = None;
    for_each_term_posting(searcher, fields.event_id, &term, |address| {
        let candidate = spill.record(address, "event_origin")?;
        if candidate.event != event || resolved.is_some() {
            return Err(IndexError::InvalidEventOriginGraph(
                "origin event identity is ambiguous",
            ));
        }
        resolved = Some(candidate);
        Ok(())
    })?;
    resolved.ok_or(IndexError::InvalidEventOriginGraph(
        "origin event does not exist",
    ))
}

fn resolve_session(
    searcher: &Searcher,
    fields: Fields,
    spill: &VerificationSpill,
    session: CompactIdentity,
) -> Result<SessionRelationship> {
    let term = Term::from_field_text(fields.session_id, &session.as_uuid().to_string());
    let mut resolved = None;
    for_each_term_posting(searcher, fields.session_id, &term, |address| {
        if resolved.is_none() {
            let candidate = spill.record(address, "session_relationship")?;
            if candidate.session != session {
                return Err(IndexError::InvalidSessionRelationshipGraph(
                    "session identity digest is ambiguous",
                ));
            }
            resolved = Some(relationship_for(candidate));
        }
        Ok(())
    })?;
    resolved.ok_or(IndexError::InvalidSessionRelationshipGraph(
        "related session does not exist",
    ))
}

fn relationship_for(identities: SpillVerificationIdentities) -> SessionRelationship {
    SessionRelationship {
        parent: identities.parent_session,
        root: identities.root_session,
        kind: identities.session_relationship,
    }
}

fn for_each_term_posting(
    searcher: &Searcher,
    field: tantivy::schema::Field,
    term: &Term,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let inverted = segment.inverted_index(field)?;
        let Some(term_info) = inverted.get_term_info(term)? else {
            continue;
        };
        for_each_live_posting(&inverted, &term_info, segment_ord, segment, &mut visit)?;
    }
    Ok(())
}

fn for_each_term_intersection(
    searcher: &Searcher,
    left_field: tantivy::schema::Field,
    left_term: &Term,
    right_field: tantivy::schema::Field,
    right_term: &Term,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let left_inverted = segment.inverted_index(left_field)?;
        let Some(left_info) = left_inverted.get_term_info(left_term)? else {
            continue;
        };
        let right_inverted = segment.inverted_index(right_field)?;
        let Some(right_info) = right_inverted.get_term_info(right_term)? else {
            continue;
        };
        let mut left =
            left_inverted.read_postings_from_terminfo(&left_info, IndexRecordOption::Basic)?;
        let mut right =
            right_inverted.read_postings_from_terminfo(&right_info, IndexRecordOption::Basic)?;
        let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
        let mut left_doc = left.doc();
        let mut right_doc = right.doc();
        while left_doc != TERMINATED && right_doc != TERMINATED {
            match left_doc.cmp(&right_doc) {
                Ordering::Less => left_doc = left.seek(right_doc),
                Ordering::Greater => right_doc = right.seek(left_doc),
                Ordering::Equal => {
                    if !segment.is_deleted(left_doc) {
                        visit(DocAddress::new(segment_ord, left_doc))?;
                    }
                    left_doc = left.advance();
                    right_doc = right.advance();
                }
            }
        }
    }
    Ok(())
}

fn for_each_live_posting(
    inverted: &tantivy::InvertedIndexReader,
    term_info: &tantivy::postings::TermInfo,
    segment_ord: usize,
    segment: &tantivy::SegmentReader,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    let mut postings = inverted.read_postings_from_terminfo(term_info, IndexRecordOption::Basic)?;
    let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
    let mut doc_id = postings.doc();
    while doc_id != TERMINATED {
        if !segment.is_deleted(doc_id) {
            visit(DocAddress::new(segment_ord, doc_id))?;
        }
        doc_id = postings.advance();
    }
    Ok(())
}
